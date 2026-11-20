use crate::interp::symbols::intern;
//! src/wasm.rs — фасад для браузерной песочницы (target wasm32-unknown-unknown).
//! Сессия = Hal(headless) + интерпретатор; кадр ведёт JS (requestAnimationFrame),
//! движку отдаётся dt. Никакого fs: скрипт приходит строкой, print уходит в
//! синк, события физики — в плоский f32-буфер (звук в песочнице).

use crate::gc;
use crate::hal::{Hal, OutputMode};
use crate::interp::{self, apply, env_get, fmt_value, parse_program, CoreCtx, GlobalCell, Value};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

pub struct Session {
    hal: Rc<RefCell<Hal>>,
    cell: GlobalCell,
    _ctx: CoreCtx,
    pin: gc::GcPin,
}

impl Session {
    pub fn new(w: usize, h: usize) -> Session {
        // паники в wasm не доходят до JS-консоли (stderr выброшен) —
        // ловим сообщение в LAST_ERR, чтобы песочница показала причину
        std::panic::set_hook(Box::new(|info| {
            let msg = format!("panic: {}", info);
            LAST_ERR.with(|c| {
                let mut b = c.borrow_mut();
                if b.is_empty() {
                    *b = msg;
                }
            });
        }));
        let halrc = Rc::new(RefCell::new(Hal::new(OutputMode::Silent, w, h)));
        let (ctx, cell) = crate::make_context(halrc.clone(), PathBuf::from("."));
        Session {
            hal: halrc,
            cell,
            _ctx: ctx,
            pin: gc::pin(vec![Value::Nil]),
        }
    }

    /// загрузить/выполнить программу (аналог load_file без fs)
    pub fn load(&self, code: &str) -> Result<(), String> {
        let prog = parse_program(code)?;
        // все формы живут до конца загрузки (как в interp::load_file) —
        // иначе авто-сборка между формами освобождает ещё не исполненные
        let pin = gc::pin(prog.clone());
        let g = *self.cell.borrow();
        for e in &prog {
            let v = interp::eval(e, g, 0)?;
            gc::pin_replace(&self.pin, vec![v]);
            gc::maybe_collect(&[g], &[]);
        }
        Ok(())
    }

    /// один кадр: update(dt) → draw() → frame-end()
    pub fn frame(&mut self, dt: f64) -> Result<(), String> {
        let g = *self.cell.borrow();
        if let Some(f) = env_get(g, "update") {
            apply(&f, &[Value::Float(dt)], 0)?;
        }
        if let Some(f) = env_get(g, "draw") {
            apply(&f, &[], 0)?;
        }
        if let Some(f) = env_get(g, "frame-end") {
            apply(&f, &[], 0)?;
        }
        Ok(())
    }

    /// REPL: вычислить выражение в живом окружении, вернуть текст результата
    pub fn eval(&self, code: &str) -> Result<String, String> {
        let prog = parse_program(code)?;
        let pin = gc::pin(prog.clone());
        let g = *self.cell.borrow();
        let mut last = Value::Nil;
        for e in &prog {
            last = interp::eval(e, g, 0)?;
            gc::pin_replace(&self.pin, vec![last.clone()]);
            gc::maybe_collect(&[g], &[]);
        }
        Ok(fmt_value(&last))
    }

    pub fn key(&mut self, k: &str, down: bool) {
        let kind = if down { "key-down" } else { "key-up" };
        self.hal.borrow_mut().queue.push(Value::List(vec![
            Value::Sym(intern(&kind.to_string())),
            Value::Str(k.to_string()),
        ]));
    }

    /// размер сцены мог смениться (window-create) или запросил UI
    pub fn resize(&mut self, w: usize, h: usize) -> Result<(), String> {
        self.hal.borrow_mut().resize_window(w, h);
        let g = *self.cell.borrow();
        for (name, val) in [("*screen-w*", w), ("*screen-h*", h)] {
            self.eval(&format!("(set! {} {})", name, val))?;
        }
        Ok(())
    }

    pub fn fb(&self) -> (*const u8, usize, usize) {
        let h = self.hal.borrow();
        (h.fb.as_ptr(), h.w, h.h)
    }
}

// ——— ошибки в статическую строку (JS читает rl_last_err) ———
thread_local! {
    static LAST_ERR: RefCell<String> = RefCell::new(String::new());
}

fn set_err(e: String) {
    LAST_ERR.with(|c| *c.borrow_mut() = e);
}

// ——— события физики для звука ———
fn drain_events(out: &mut Vec<f32>) {
    crate::physics::drain_js_events(out);
}

// ——— C ABI ———
// Передача строк/данных — через два статических буфера (wasm однопоточен):
// IN — JS пишет сюда скрипты/выражения/имена клавиш,
// OUT — движок пишет события физики (f32), вывод print'ов и ошибки/результат.
// Указатели стабильны: вектора фиксированной длины, реаллокаций нет.

thread_local! {
    static BUF_IN: RefCell<Vec<u8>> = RefCell::new(vec![0u8; 262_144]);
    static BUF_OUT: RefCell<Vec<u8>> = RefCell::new(vec![0u8; 262_144]);
}

#[no_mangle]
pub extern "C" fn rl_scratch_in() -> *mut u8 {
    BUF_IN.with(|b| b.borrow_mut().as_mut_ptr())
}

#[no_mangle]
pub extern "C" fn rl_scratch_out() -> *mut u8 {
    BUF_OUT.with(|b| b.borrow_mut().as_mut_ptr())
}

#[no_mangle]
pub extern "C" fn rl_new(w: i32, h: i32) -> *mut Session {
    let w = w.clamp(64, 1280) as usize;
    let h = h.clamp(64, 1280) as usize;
    Box::into_raw(Box::new(Session::new(w, h)))
}

/// # Safety
/// p должен быть указателем от rl_new; после вызова p невалиден
#[no_mangle]
pub unsafe extern "C" fn rl_free(p: *mut Session) {
    if !p.is_null() {
        drop(Box::from_raw(p));
    }
}

fn utf8_in(len: usize) -> String {
    BUF_IN.with(|b| {
        let bb = b.borrow();
        let n = len.min(262_144).min(bb.len());
        String::from_utf8_lossy(&bb[..n]).into_owned()
    })
}

/// загрузить и выполнить программу из BUF_IN[0..len]; 0 — ок, −1 — ошибка
#[no_mangle]
pub unsafe extern "C" fn rl_load(p: *mut Session, _ptr: *const u8, len: usize) -> i32 {
    let s = utf8_in(len);
    match (*p).load(&s) {
        Ok(()) => 0,
        Err(e) => {
            set_err(e);
            -1
        }
    }
}

/// REPL: вычислить BUF_IN[0..len] в живом окружении; результат — в BUF_OUT
#[no_mangle]
pub unsafe extern "C" fn rl_eval(p: *mut Session, _ptr: *const u8, len: usize) -> i32 {
    let s = utf8_in(len);
    match (*p).eval(&s) {
        Ok(t) => {
            set_err(t); // результат читается через rl_last_err
            0
        }
        Err(e) => {
            set_err(format!("⚠ {}", e));
            -1
        }
    }
}

/// кадр: update(dt) → draw() → frame-end(); wall_ms — performance.now() из JS.
/// 0 — ок, −1 — ошибка
#[no_mangle]
pub unsafe extern "C" fn rl_frame(p: *mut Session, dt: f32, wall_ms: f64) -> i32 {
    crate::hal::set_js_now(wall_ms);
    let dt = dt.clamp(0.001, 0.05) as f64;
    match (*p).frame(dt) {
        Ok(()) => 0,
        Err(e) => {
            set_err(e);
            -1
        }
    }
}

/// клавиша из BUF_IN[0..len]; down != 0 — нажата
#[no_mangle]
pub unsafe extern "C" fn rl_key(p: *mut Session, _ptr: *const u8, len: usize, down: i32) {
    let s = utf8_in(len);
    (*p).key(&s, down != 0);
}

#[no_mangle]
pub unsafe extern "C" fn rl_resize(p: *mut Session, w: i32, h: i32) -> i32 {
    match (*p).resize(w.clamp(64, 1280) as usize, h.clamp(64, 1280) as usize) {
        Ok(()) => 0,
        Err(e) => {
            set_err(e);
            -1
        }
    }
}

/// указатель на RGBA-фреймбуфер (w·h·4 байт)
#[no_mangle]
pub unsafe extern "C" fn rl_fb(p: *mut Session) -> *const u8 {
    (*p).fb().0
}

#[no_mangle]
pub unsafe extern "C" fn rl_w(p: *mut Session) -> i32 {
    (*p).fb().1 as i32
}

#[no_mangle]
pub unsafe extern "C" fn rl_h(p: *mut Session) -> i32 {
    (*p).fb().2 as i32
}

/// события физики в BUF_OUT (f32: a,kind,speed,x,y,z ×n) → число f32
#[no_mangle]
pub unsafe extern "C" fn rl_take_events(_p: *mut Session, cap: usize) -> usize {
    let mut v: Vec<f32> = Vec::new();
    drain_events(&mut v);
    let n = v.len().min(cap);
    let bytes: Vec<u8> = v[..n].iter().flat_map(|f| f.to_le_bytes()).collect();
    BUF_OUT.with(|b| {
        let mut ob = b.borrow_mut();
        ob[..bytes.len()].copy_from_slice(&bytes);
    });
    n
}

/// вывод print'ов в BUF_OUT → число байт
#[no_mangle]
pub unsafe extern "C" fn rl_take_output(_cap: usize) -> usize {
    let s = interp::take_out_sink();
    let b = s.as_bytes();
    let n = b.len().min(262_144);
    BUF_OUT.with(|o| {
        let mut ob = o.borrow_mut();
        ob[..n].copy_from_slice(&b[..n]);
    });
    n
}

/// последняя ошибка/результат rl_eval в BUF_OUT → число байт
#[no_mangle]
pub unsafe extern "C" fn rl_last_err(_cap: usize) -> usize {
    LAST_ERR.with(|c| {
        let s = c.borrow();
        let b = s.as_bytes();
        let n = b.len().min(262_144);
        BUF_OUT.with(|o| {
            let mut ob = o.borrow_mut();
            ob[..n].copy_from_slice(&b[..n]);
        });
        n
    })
}

/// суммарный объём встроенных Lisp-исходников (диагностика песочницы)
#[no_mangle]
pub extern "C" fn rl_probe_embedded() -> usize {
    crate::interp::EMBEDDED_LISP
        .iter()
        .map(|(_, s)| s.len())
        .sum()
}
