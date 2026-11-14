// src/interp/builtins_misc.rs — нативы, которых не хватало сцене для
// нормальной игровой логики: сортировка со своим компаратором, интерполяция,
// строки. Всё детерминировано: `sort` — устойчивая сортировка с явным
// разрешением равных элементов, никаких хеш-порядков.

use std::rc::Rc;

use crate::gc::Gc;
use crate::interp::builtins::list_elems;
use crate::interp::env::env_define;
use crate::interp::eval::apply;
use crate::interp::value::{fmt_value, int_val, need_args as need, num_val, Value};

fn as_f64(v: &Value) -> Result<f64, String> {
    num_val(v)
}

/// Естественный порядок для sort без компаратора: числа < строки < прочее,
/// внутри — по значению; сравнение полное, чтобы сортировка была предсказуема.
fn natural_cmp(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Int(x), Value::Float(y)) => (*x as f64).partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Float(x), Value::Int(y)) => x.partial_cmp(&(*y as f64)).unwrap_or(Ordering::Equal),
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        (Value::Sym(x), Value::Sym(y)) => x.cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        _ => fmt_value(a).cmp(&fmt_value(b)),
    }
}

pub fn register_misc_builtins(env: Gc) {
    fn def<F: Fn(&[Value]) -> Result<Value, String> + 'static>(env: Gc, name: &str, f: F) {
        env_define(env, name, Value::Native(Rc::new(f), name.to_string()));
    }

    // (sort lst) / (sort lst cmp): cmp(a, b) истинно, если a должен идти раньше b.
    // Устойчиво: равные элементы сохраняют исходный порядок — это важно для
    // сцен, где сортировка по y не должна тасовать врагов с одинаковым y.
    def(env, "sort", |args| {
        if args.is_empty() || args.len() > 2 {
            return Err("sort: нужно (sort список [компаратор])".into());
        }
        let mut items = list_elems(&args[0], "sort")?;
        let cmp = args.get(1).cloned();
        match cmp {
            Some(f) if !matches!(f, Value::Nil) => {
                // вставками: стабильно и без паники в компараторе (ошибка
                // компаратора пробрасывается как ошибка сцены)
                let mut out: Vec<Value> = Vec::with_capacity(items.len());
                for item in items.drain(..) {
                    let mut pos = out.len();
                    while pos > 0 {
                        let before = apply(&f, &[item.clone(), out[pos - 1].clone()], 0)?;
                        if truthy(&before) {
                            pos -= 1;
                        } else {
                            break;
                        }
                    }
                    out.insert(pos, item);
                }
                Ok(Value::List(out))
            }
            _ => {
                items.sort_by(natural_cmp);
                Ok(Value::List(items))
            }
        }
    });

    // (lerp a b t) — линейная интерполяция; t не зажимается (экстраполяция
    // иногда нужна для предсказания позиции)
    def(env, "lerp", |args| {
        need(args, 3)?;
        let a = as_f64(&args[0])?;
        let b = as_f64(&args[1])?;
        let t = as_f64(&args[2])?;
        Ok(Value::Float(a + (b - a) * t))
    });

    // (smoothstep e0 e1 x) — плавная ступенька, зажатая в [0,1]
    def(env, "smoothstep", |args| {
        need(args, 3)?;
        let e0 = as_f64(&args[0])?;
        let e1 = as_f64(&args[1])?;
        let x = as_f64(&args[2])?;
        if (e1 - e0).abs() < f64::EPSILON {
            return Ok(Value::Float(if x < e0 { 0.0 } else { 1.0 }));
        }
        let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
        Ok(Value::Float(t * t * (3.0 - 2.0 * t)))
    });

    // (sign x) → -1 / 0 / 1 как число с плавающей точкой
    def(env, "sign", |args| {
        need(args, 1)?;
        let x = as_f64(&args[0])?;
        Ok(Value::Float(if x > 0.0 {
            1.0
        } else if x < 0.0 {
            -1.0
        } else {
            0.0
        }))
    });

    // (approach cur target step) — шаг к цели, не перескакивая
    def(env, "approach", |args| {
        need(args, 3)?;
        let cur = as_f64(&args[0])?;
        let target = as_f64(&args[1])?;
        let step = as_f64(&args[2])?.abs();
        let d = target - cur;
        if d.abs() <= step {
            Ok(Value::Float(target))
        } else {
            Ok(Value::Float(cur + d.signum() * step))
        }
    });

    // —— строки ————————————————————————————————————————————————
    fn str_arg(v: &Value, what: &str) -> Result<String, String> {
        match v {
            Value::Str(s) | Value::Sym(s) => Ok(s.clone()),
            other => Err(format!(
                "{}: ожидалась строка, получено {}",
                what,
                fmt_value(other)
            )),
        }
    }

    // (string-split "a,b" ",") → ("a" "b"); пустой разделитель — по символам
    def(env, "string-split", |args| {
        need(args, 2)?;
        let s = str_arg(&args[0], "string-split")?;
        let sep = str_arg(&args[1], "string-split")?;
        let parts: Vec<Value> = if sep.is_empty() {
            s.chars().map(|c| Value::Str(c.to_string())).collect()
        } else {
            s.split(sep.as_str())
                .map(|p| Value::Str(p.to_string()))
                .collect()
        };
        Ok(Value::List(parts))
    });

    // (string-join ("a" "b") ", ") → "a, b"
    def(env, "string-join", |args| {
        need(args, 2)?;
        let items = list_elems(&args[0], "string-join")?;
        let sep = str_arg(&args[1], "string-join")?;
        let mut parts = Vec::with_capacity(items.len());
        for it in items {
            parts.push(str_arg(&it, "string-join")?);
        }
        Ok(Value::Str(parts.join(&sep)))
    });

    // (string-substring s start [end]) — посимвольно (не побайтово: UTF-8)
    def(env, "string-substring", |args| {
        if args.len() < 2 || args.len() > 3 {
            return Err("string-substring: нужно (s начало [конец])".into());
        }
        let s = str_arg(&args[0], "string-substring")?;
        let chars: Vec<char> = s.chars().collect();
        let n = chars.len() as i64;
        let start = int_val(&args[1])?.clamp(0, n) as usize;
        let end = match args.get(2) {
            Some(v) => int_val(v)?.clamp(0, n) as usize,
            None => chars.len(),
        };
        let end = end.max(start);
        Ok(Value::Str(chars[start..end].iter().collect()))
    });

    // (string-find "hay" "needle") → индекс символа или -1
    def(env, "string-find", |args| {
        need(args, 2)?;
        let hay = str_arg(&args[0], "string-find")?;
        let needle = str_arg(&args[1], "string-find")?;
        match hay.find(&needle) {
            Some(byte) => Ok(Value::Int(hay[..byte].chars().count() as i64)),
            None => Ok(Value::Int(-1)),
        }
    });

    def(env, "string-upcase", |args| {
        need(args, 1)?;
        Ok(Value::Str(
            str_arg(&args[0], "string-upcase")?.to_uppercase(),
        ))
    });

    def(env, "string-downcase", |args| {
        need(args, 1)?;
        Ok(Value::Str(
            str_arg(&args[0], "string-downcase")?.to_lowercase(),
        ))
    });

    def(env, "string-trim", |args| {
        need(args, 1)?;
        Ok(Value::Str(
            str_arg(&args[0], "string-trim")?.trim().to_string(),
        ))
    });

    // (string-replace s from to) — все вхождения
    def(env, "string-replace", |args| {
        need(args, 3)?;
        let s = str_arg(&args[0], "string-replace")?;
        let from = str_arg(&args[1], "string-replace")?;
        let to = str_arg(&args[2], "string-replace")?;
        if from.is_empty() {
            return Ok(Value::Str(s));
        }
        Ok(Value::Str(s.replace(&from, &to)))
    });
}

fn truthy(v: &Value) -> bool {
    !matches!(v, Value::Nil | Value::Bool(false))
}
