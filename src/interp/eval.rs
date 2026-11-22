// src/interp/eval.rs — Древовидный интерпретатор, аппликация функций и макрорасширение.

use super::{
    as_list, as_sym, chain_parts, env_define, env_get, env_set, fmt_value, is_sym, truthy,
    vec_into_chain, Value,
};
use crate::gc::{self, Gc, LambdaData};

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
                        // Этап 3: JIT числовых ядер — гард (арность/типы) в
                        // call_jitted; провал → честный откат в интерпретатор
                        #[cfg(feature = "jit")]
                        {
                            if let Some(v) = crate::jit::try_call_lambda(g, &args) {
                                return Ok(v);
                            }
                        }
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
        Value::Sym(s) => Ok((vec![], Some(s.to_string()))),
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
            // Этап 3: JIT числовых ядер (фича `jit`); на провале гарда/компиляции —
            // прозрачная деоптимизация в интерпретатор (см. src/jit.rs, LIMITATIONS.md)
            #[cfg(feature = "jit")]
            {
                if let Some(v) = crate::jit::try_call_lambda(*g, args) {
                    return Ok(v);
                }
            }
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
