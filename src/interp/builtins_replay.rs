// src/interp/builtins_replay.rs — нативы детерминизма и записи прогона.
//
// Малый, но принципиальный набор: сцена (Lisp) должна уметь спросить
// «а воспроизводится ли то, что я вижу?» и получить ответ числом, а не
// обещанием. Поэтому здесь только seed ГПСЧ и управление сессией записи;
// сам кадр и его hash считает Rust (src/replay.rs) в основном цикле.

use std::rc::Rc;

use crate::gc::Gc;
use crate::interp::env::env_define;
use crate::interp::rng::{ensure_seed, initial_seed, next_random, set_seed};
use crate::interp::value::{int_val, num_val, Value};
use crate::replay;

pub fn register_replay_builtins(env: Gc) {
    fn def<F: Fn(&[Value]) -> Result<Value, String> + 'static>(env: Gc, name: &str, f: F) {
        env_define(env, name, Value::Native(Rc::new(f), name.to_string()));
    }

    // ——— детерминированный ГПСЧ ———
    def(env, "random-seed", |args| {
        if args.is_empty() {
            return Ok(Value::Int(ensure_seed() as i64));
        }
        let s = int_val(&args[0])? as u64;
        set_seed(s);
        Ok(Value::Int(s as i64))
    });
    def(env, "random-int", |args| {
        if args.len() != 2 {
            return Err("random-int: нужно (random-int НИЖЕ ВЕРХ) — оба включительно".into());
        }
        let lo = int_val(&args[0])?;
        let hi = int_val(&args[1])?;
        if hi < lo {
            return Err(format!("random-int: пустой диапазон {}..{}", lo, hi));
        }
        let span = (hi - lo + 1) as f64;
        let r = next_random();
        Ok(Value::Int(lo + (r * span) as i64))
    });
    def(env, "random-range", |args| {
        if args.len() != 2 {
            return Err("random-range: нужно (random-range НИЗ ВЕРХ)".into());
        }
        let lo = num_val(&args[0])?;
        let hi = num_val(&args[1])?;
        let r = next_random();
        Ok(Value::Float(lo + (hi - lo) * r))
    });

    // ——— запись и воспроизведение прогона ———
    def(env, "replay-record!", |args| {
        let path = match args.first() {
            Some(Value::Str(s)) => s.clone(),
            Some(Value::Sym(s)) => s.to_string(),
            _ => return Err("replay-record!: нужно (replay-record! \"путь.rlrp\")".into()),
        };
        // в файл идёт seed НАЧАЛА прогона: запись, начатая с середины,
        // воспроизводится прогоном с нуля и сверкой кадров с start_frame
        let seed = initial_seed();
        let fps = 60.0;
        replay::start_record(path.clone(), seed, fps);
        Ok(Value::Str(format!(
            "запись начата: {} (seed {}) — останови (replay-stop!)",
            path, seed
        )))
    });
    def(env, "replay-stop!", |_| match replay::stop() {
        Some(report) => Ok(Value::Str(report)),
        None => Ok(Value::Bool(false)),
    });
    def(env, "replay-load!", |args| {
        let path = match args.first() {
            Some(Value::Str(s)) => s.clone(),
            Some(Value::Sym(s)) => s.to_string(),
            _ => return Err("replay-load!: нужно (replay-load! \"путь.rlrp\")".into()),
        };
        match replay::start_replay(&path) {
            Ok(seed) => {
                // воспроизведение обязано идти с тем же seed, что и запись
                set_seed(seed);
                Ok(Value::Str(format!(
                    "воспроизведение: {} (seed {})",
                    path, seed
                )))
            }
            Err(e) => Err(e),
        }
    });
    def(env, "replay-active?", |_| Ok(Value::Bool(replay::active())));
    def(env, "replay-status", |_| Ok(replay::status_table()));
    def(env, "replay-report", |_| {
        Ok(Value::Str(replay::status_line()))
    });
}
