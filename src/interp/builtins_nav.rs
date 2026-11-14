// src/interp/builtins_nav.rs — нативы навигации: решётка и A* из Lisp.
//
// Сцена спрашивает «дай путь» и получает плоский список клеток; всё
// остальное (сколько врагов, как часто пересчитывать, как сглаживать) —
// дело Lisp-кода, а не ядра.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Mutex;

use crate::gc::Gc;
use crate::interp::env::env_define;
use crate::interp::value::{int_val, need_args as need, Value};
use crate::nav::NavGrid;

// Реестр сеток общий на процесс, а не thread_local: сцена инициализируется
// на одной нити, а кадры крутятся на другой (игровая нить), и thread_local
// там — разные векторы. Мьютекс здесь ничего не стоит: A* зовут десятки раз
// за кадр, а не миллионы.
static NAVS: Mutex<Vec<Option<NavGrid>>> = Mutex::new(Vec::new());

fn with_grid<R>(h: i64, f: impl FnOnce(&mut NavGrid) -> Result<R, String>) -> Result<R, String> {
    let mut v = NAVS
        .lock()
        .map_err(|_| "nav: реестр отравлен".to_string())?;
    let idx = h as usize;
    match v.get_mut(idx) {
        Some(Some(g)) => f(g),
        Some(None) => Err(format!("nav: сетка {} удалена", h)),
        None => Err(format!("nav: нет сетки {}", h)),
    }
}

fn registry_len() -> usize {
    NAVS.lock()
        .map(|v| v.iter().filter(|x| x.is_some()).count())
        .unwrap_or(0)
}

pub fn register_nav_builtins(env: Gc) {
    fn def<F: Fn(&[Value]) -> Result<Value, String> + 'static>(env: Gc, name: &str, f: F) {
        env_define(env, name, Value::Native(Rc::new(f), name.to_string()));
    }

    def(env, "nav-new", |args| {
        need(args, 2)?;
        let w = int_val(&args[0])?.max(1) as usize;
        let h = int_val(&args[1])?.max(1) as usize;
        if w * h > 4_000_000 {
            return Err(format!("nav-new: сетка {}x{} слишком большая", w, h));
        }
        let mut v = NAVS
            .lock()
            .map_err(|_| "nav: реестр отравлен".to_string())?;
        v.push(Some(NavGrid::new(w, h)));
        Ok(Value::Int((v.len() - 1) as i64))
    });

    def(env, "nav-block!", |args| {
        need(args, 4)?;
        let h = int_val(&args[0])?;
        let x = int_val(&args[1])?;
        let y = int_val(&args[2])?;
        let blocked = matches!(&args[3], Value::Bool(true));
        with_grid(h, |g| g.set_blocked(x, y, blocked).map(|_| Value::Nil))
    });

    def(env, "nav-walkable!", |args| {
        need(args, 4)?;
        let h = int_val(&args[0])?;
        let x = int_val(&args[1])?;
        let y = int_val(&args[2])?;
        let walkable = matches!(&args[3], Value::Bool(true));
        with_grid(h, |g| g.set_walkable(x, y, walkable).map(|_| Value::Nil))
    });

    def(env, "nav-free?", |args| {
        need(args, 3)?;
        let h = int_val(&args[0])?;
        let x = int_val(&args[1])?;
        let y = int_val(&args[2])?;
        with_grid(h, |g| Ok(Value::Bool(g.free(x, y))))
    });

    def(env, "nav-open", |args| {
        need(args, 3)?;
        let h = int_val(&args[0])?;
        let x = int_val(&args[1])?;
        let y = int_val(&args[2])?;
        with_grid(h, |g| Ok(Value::Int(g.open_neighbors(x, y) as i64)))
    });

    // (nav-path h x0 y0 x1 y1) → (x0 y0 x1 y1 …) или #f
    def(env, "nav-path", |args| {
        need(args, 5)?;
        let h = int_val(&args[0])?;
        let x0 = int_val(&args[1])?;
        let y0 = int_val(&args[2])?;
        let x1 = int_val(&args[3])?;
        let y1 = int_val(&args[4])?;
        with_grid(h, |g| match g.path(x0, y0, x1, y1) {
            Some(cells) => Ok(Value::List(
                cells.into_iter().map(|c| Value::Int(c as i64)).collect(),
            )),
            None => Ok(Value::Bool(false)),
        })
    });

    def(env, "nav-stats", |args| {
        need(args, 1)?;
        let h = int_val(&args[0])?;
        with_grid(h, |g| {
            let mut t = std::collections::HashMap::new();
            t.insert("w".to_string(), Value::Int(g.w as i64));
            t.insert("h".to_string(), Value::Int(g.h as i64));
            t.insert(
                "blocked".to_string(),
                Value::Int(g.blocked.iter().filter(|b| **b).count() as i64),
            );
            t.insert("searches".to_string(), Value::Int(g.searches as i64));
            t.insert("last-len".to_string(), Value::Int(g.last_len as i64));
            Ok(Value::Table(Rc::new(RefCell::new(t))))
        })
    });

    def(env, "nav-clear!", |args| {
        need(args, 1)?;
        let h = int_val(&args[0])?;
        with_grid(h, |g| {
            for b in g.blocked.iter_mut() {
                *b = false;
            }
            g.searches = 0;
            g.last_len = 0;
            Ok(Value::Nil)
        })
    });

    def(env, "nav-count", |_| Ok(Value::Int(registry_len() as i64)));
}
