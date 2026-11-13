// src/editor/lisp.rs — Lisp-сторона редактора: хуки, REPL, публикация сцены,
// исполнение команд моста в игровой нити.

use super::bridge::Bridge;
use super::server::bridge;
use crate::interp::{env_get, fmt_value, parse_program, CoreCtx, Gc, GlobalCell, Value};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// Натив (editor-log msg) — Lisp пишет в консоль редактора (или в stderr).
pub fn register_editor(env: Gc) {
    crate::interp::env_define(
        env,
        "editor-log",
        Value::Native(
            Rc::new(move |args: &[Value]| {
                let parts: Vec<String> = args.iter().map(fmt_value).collect();
                let line = parts.join(" ");
                match bridge() {
                    Some(b) => b.log_line(line),
                    None => eprintln!("[lisp] {}", line),
                }
                Ok(Value::Nil)
            }),
            "editor-log".to_string(),
        ),
    );
}

/// Исполнить Lisp-строку в живом окружении (команды редактора/MCP).
pub fn eval_string(code: &str, cell: &GlobalCell) -> Result<Value, String> {
    // ошибки разбора тоже получают координату: парсер позиций не хранит,
    // их восстанавливает editor::diagnostics по фрагменту из текста ошибки
    let prog = parse_program(code).map_err(|e| super::diagnostics::annotate(code, None, &e))?;
    // формы (могут нести Cons-литералы) и последний результат переживают
    // сборки на границах форм — pin-корни
    let _pin = crate::gc::pin(prog.clone());
    let g = *cell.borrow();
    let mut last = Value::Nil;
    let pin = crate::gc::pin(vec![last.clone()]);
    for (i, e) in prog.into_iter().enumerate() {
        last = crate::interp::eval(&e, g, 0)
            .map_err(|err| super::diagnostics::annotate(code, Some(i), &err))?;
        crate::gc::pin_replace(&pin, vec![last.clone()]);
        crate::gc::maybe_collect(&[g], &[]);
    }
    drop(pin);
    Ok(last)
}

/// Публикация сцены: (editor-scene) из Lisp → JSON-строка в Bridge.
pub fn publish_scene(cell: &GlobalCell, bridge: &Arc<Bridge>) {
    let g = *cell.borrow();
    if let Some(f) = env_get(g, "editor-scene") {
        match crate::interp::apply(&f, &[], 0) {
            Ok(v) => {
                *bridge.scene.lock().unwrap() = crate::json::value_to_json(&v).dump();
            }
            Err(e) => bridge.log_line(format!("[scene] ошибка: {}", e)),
        }
    }
}

/// Этап 6: публикация хуков камеры/префабов — (editor-camera) и (prefab-list),
/// если игра их объявила. Нет хука — в Bridge остаётся «null», редактор честно
/// прячет гизмо/префабы.
pub fn publish_extras(cell: &GlobalCell, bridge: &Arc<Bridge>) {
    let g = *cell.borrow();
    if let Some(f) = env_get(g, "editor-camera") {
        if let Ok(v) = crate::interp::apply(&f, &[], 0) {
            *bridge.camera.lock().unwrap() = crate::json::value_to_json(&v).dump();
        }
    }
    if let Some(f) = env_get(g, "prefab-list") {
        if let Ok(v) = crate::interp::apply(&f, &[], 0) {
            *bridge.prefabs.lock().unwrap() = crate::json::value_to_json(&v).dump();
        }
    }
    // детерминизм: строка «mode=… frames=… chain=…» для панели редактора
    *bridge.replay.lock().unwrap() = crate::replay::status_line();
}

/// Игровая нить: разбор команд редактора (eval/keys/ctl-запросы reload).
/// Возвращает true, если была команда reload.
pub fn drain_editor_cmds(
    bridge: &Arc<Bridge>,
    cell: &GlobalCell,
    halrc: &Rc<RefCell<crate::hal::Hal>>,
) -> bool {
    let mut reload = false;
    if bridge.reload_req.swap(false, Ordering::Relaxed) {
        reload = true;
    }
    let cmds: Vec<(u64, String)> = std::mem::take(&mut *bridge.cmds.lock().unwrap());
    for (id, code) in cmds {
        let res = eval_string(&code, cell);
        let text = match res {
            Ok(v) => fmt_value(&v),
            Err(e) => format!("ОШИБКА: {}", e),
        };
        bridge.results.lock().unwrap().push((id, text.clone()));
        let mut rq = bridge.results.lock().unwrap();
        while rq.len() > 64 {
            rq.remove(0);
        }
        bridge.log_line(format!(
            "[#{}] {}",
            id,
            text.lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(120)
                .collect::<String>()
        ));
    }
    let keys: Vec<(String, bool)> = std::mem::take(&mut *bridge.keys.lock().unwrap());
    if !keys.is_empty() {
        let mut hal = halrc.borrow_mut();
        for (k, down) in keys {
            let ev = if down { "key-down" } else { "key-up" };
            hal.queue
                .push(Value::List(vec![Value::Sym(ev.into()), Value::Str(k)]));
        }
    }
    reload
}

/// Контекст для MCP/eval-инструментов.
pub struct EngineHandle {
    pub ctx: CoreCtx,
    pub cell: GlobalCell,
    pub halrc: Rc<RefCell<crate::hal::Hal>>,
}

/// Шаг кадра для MCP-режима (update → draw → frame-end).
pub fn step_frame(h: &EngineHandle, dt: f64) -> Result<(), String> {
    let g = *h.cell.borrow();
    if let Some(f) = env_get(g, "update") {
        crate::interp::apply(&f, &[Value::Float(dt)], 0)?;
    }
    if let Some(f) = env_get(g, "draw") {
        crate::interp::apply(&f, &[], 0)?;
    }
    if let Some(f) = env_get(g, "frame-end") {
        let _ = crate::interp::apply(&f, &[], 0);
    }
    // Этап 2: граница форм MCP-цикла — авто-сборка GC
    crate::gc::maybe_collect(&[g], &[]);
    Ok(())
}

// ——————————————————————————— тесты этапа 6 ———————————————————————————
