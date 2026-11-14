// src/main.rs — Rust-ядро: точка входа, инициализация окружения, регистрация подсистем.
// Горячий цикл — Lisp; Rust здесь делает только HAL-работу: время, ввод, кадры, hot reload.
//
// clippy новых линеек знает линты, которых нет в 1.83 (MSRV поддерживаемого
// rustc): manual_is_multiple_of требует std 1.87 — на 1.83 используем `%`.
#![allow(unknown_lints)]
#![allow(clippy::manual_is_multiple_of)]

mod assets;
mod audio;
mod bench;
pub mod cli;
mod ecs;
pub mod editor;
pub mod gc;
pub mod gpu;
mod gpu_3d;
pub mod hal;
pub mod interp;
#[cfg(feature = "jit")]
pub mod jit;
mod json;
pub mod mcp;
mod nav;
#[cfg(test)]
mod parse_diff;
mod particles;
mod physics;
mod png;
mod post;
pub mod render3d;
pub mod replay;
pub mod runner;
pub mod simd;
mod skel;
#[cfg(target_arch = "wasm32")]
mod wasm;

use hal::Hal;
use interp::{env_define, env_new, CoreCtx, Gc, GlobalCell, LoadRegistry, Value};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

pub use cli::Args;
pub use runner::{reload_all, snapshot_mtimes};

pub fn register_all(env: Gc, ctx: &CoreCtx, halrc: &Rc<RefCell<Hal>>) {
    interp::register_core(env, ctx);
    hal::register_hal(env, halrc, ctx.root.clone());
    simd::register_simd(env);
    render3d::register_render3d(env, halrc);
    editor::register_editor(env);
    physics::register(env, Rc::new(RefCell::new(Vec::new())));
    ecs::register(env, Rc::new(RefCell::new(Vec::new())));
    particles::register(env, Rc::new(RefCell::new(Vec::new())));
    skel::register(env, Rc::new(RefCell::new(Vec::new())));
    assets::register(env, Rc::new(RefCell::new(Vec::new())));
}

pub fn make_context(halrc: Rc<RefCell<Hal>>, root: PathBuf) -> (CoreCtx, GlobalCell) {
    let env0 = env_new(None);
    // глобальное окружение сразу попадает в ячейку-корень: все аллокации
    // register_all достижимы с первой же возможной сборки
    let cell: GlobalCell = Rc::new(RefCell::new(env0));
    let registry: LoadRegistry = Rc::new(RefCell::new(vec![]));
    let ctx = CoreCtx {
        global: cell.clone(),
        registry,
        root,
    };
    register_all(env0, &ctx, &halrc);
    env_define(env0, "*root*", Value::Str(".".into()));
    (ctx, cell)
}

fn main() {
    let args = match cli::parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{}\n\n{}", e, cli::USAGE);
            std::process::exit(2);
        }
    };
    // калибровка SIMD-уровня по фактической скорости (см. simd.rs)
    let lvl = simd::calibrate();
    if std::env::args().any(|a| a == "--bench") {
        println!("уровень после калибровки: {}", simd::level_name(lvl));
    }
    if args.bench {
        if let Err(e) = bench::run_bench(&args) {
            eprintln!("[ошибка] {}", e);
            std::process::exit(1);
        }
        return;
    }
    if args.bench_parse {
        if let Err(e) = bench::run_bench_parse() {
            eprintln!("[ошибка] {}", e);
            std::process::exit(1);
        }
        return;
    }
    if args.mcp {
        let script = PathBuf::from(&args.script);
        if let Err(e) = mcp::run_mcp(&script, args.fps) {
            eprintln!("[mcp:ошибка] {}", e);
            std::process::exit(1);
        }
        return;
    }
    if let Some(code) = args.eval_code.clone() {
        if let Err(e) = runner::run_eval(&code) {
            eprintln!("[ошибка] {}", e);
            std::process::exit(1);
        }
        return;
    }
    // Запуск игрового цикла в потоке с гарантированным стеком 8 МБ
    // (на Windows дефолтный стек главного PE-потока всего 1 МБ, что приводит
    // к переполнению в неоптимизированной debug-сборке при глубоком разборе Lisp)
    let builder = std::thread::Builder::new()
        .name("rlisp-main".into())
        .stack_size(8 * 1024 * 1024);
    let handler = builder
        .spawn(move || {
            if let Err(e) = runner::run_engine(&args) {
                eprintln!("[ошибка] {}", e);
                std::process::exit(1);
            }
        })
        .unwrap();
    if handler.join().is_err() {
        std::process::exit(1);
    }
}
