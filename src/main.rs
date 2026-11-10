// src/main.rs — Rust-ядро: запускает Lisp-мир и вызывает update(dt)/draw() каждый кадр.
// Горячий цикл — Lisp; Rust здесь делает только HAL-работу: время, ввод, кадры, hot reload.
//
// clippy новых линеек знает линты, которых нет в 1.83 (MSRV поддерживаемого
// rustc): manual_is_multiple_of требует std 1.87 — на 1.83 используем `%`.
#![allow(unknown_lints)]
#![allow(clippy::manual_is_multiple_of)]

mod assets;
mod audio;
mod ecs;
mod editor;
mod gc;
mod gpu;
mod hal;
mod interp;
#[cfg(feature = "jit")]
mod jit;
mod json;
mod mcp;
#[cfg(test)]
mod parse_diff;
mod particles;
mod physics;
mod png;
mod post;
mod render3d;
mod simd;
mod skel;
#[cfg(target_arch = "wasm32")]
mod wasm;

use hal::{Hal, OutputMode, Tex};
use interp::{
    apply, env_define, env_get, env_new, fmt_value, load_file, parse_program, CoreCtx, Gc,
    GlobalCell, LoadRegistry, Value,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

struct Args {
    script: String,
    ascii: bool,
    capture: Option<String>,
    every: usize,
    frames: u64,
    fps: f64,
    width: usize,
    height: usize,
    input: Option<String>,
    stdin_keys: bool,
    fast: bool,
    bench: bool,
    bench_parse: bool,
    eval_code: Option<String>,
    serve: bool,
    port: u16,
    mcp: bool,
    wgpu: bool,
}

impl Default for Args {
    fn default() -> Self {
        Args {
            script: "assets/lisp/main.lisp".into(),
            ascii: false,
            capture: None,
            every: 3,
            frames: u64::MAX,
            fps: 30.0,
            width: 320,
            height: 240,
            input: None,
            stdin_keys: false,
            fast: false,
            bench: false,
            bench_parse: false,
            eval_code: None,
            serve: false,
            port: 7300,
            mcp: false,
            wgpu: false,
        }
    }
}

const USAGE: &str = "\
rlisp-engine — игровой движок как Lisp-машина (Rust = только HAL)

ЗАПУСК ДЕМО «ВЫЖИВАНИЕ» (весь игровой код — Lisp):
  cargo run --release
  cargo run --release -- --ascii --stdin-keys   # играть в терминале: wasd, q — выход

ЗАПУСК 3D-ДЕМО «ORBITAL» (3D-пайплайн на Lisp над HAL-растром):
  cargo run --release -- --script assets/lisp/main3d.lisp --ascii --stdin-keys
  cargo run --release -- --script assets/lisp/main3d.lisp --capture demo/frames3d --every 6 --frames 540 --fast

РЕЖИМЫ:
  --script FILE   точка входа Lisp (по умолчанию assets/lisp/main.lisp)
  --ascii         рендер в терминал (ANSI)
  --stdin-keys    клавиши из stdin (raw mode; строчные — нажать, заглавные — отпустить)
  --input FILE    сценарий ввода: строки «ВРЕМЯ КЛАВИША down|up»
  --capture DIR   писать кадры PNG в DIR
  --every N       каждый N-й кадр (по умолчанию 3)
  --frames N      остановиться после N кадров
  --fps F         шаг симуляции (по умолчанию 30)
  --fast          без привязки к реальному времени
  --width W --height H   стартовый размер экрана
  --bench         бенчмарк SIMD-слоя + 2D-кадра + 3D-шейдинга (этап 7)
  --bench-parse   бенчмарк токенизатора/парсера: старый посимвольный reader vs
                  новый SIMD-сканер (src/simd.rs) на реальном корпусе
                  assets/lisp/*.lisp; оба пути обязаны давать одинаковые AST
  --wgpu          GPU-бэкенд за фасадом HAL (фича `wgpu`): offscreen-рендер +
                  readback в фреймбуфер; без адаптера — откат на программный
                  растр с предупреждением (все скрипты работают как раньше)
  --eval EXPR     вычислить выражение Lisp и выйти

РЕДАКТОР И AI (v0.3):
  --serve         веб-редактор аля Unity: иерархия, инспектор, REPL,
                  живой вид, ассеты с hot reload (http://127.0.0.1:7300)
  --port N        порт для --serve (по умолчанию 7300)
  --mcp           MCP-сервер (Model Context Protocol, JSON-RPC 2.0, stdio):
                  ИИ-агент управляет движком — scene/step/eval/screenshot";

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--script" => args.script = it.next().ok_or("--script: нужен путь")?,
            "--ascii" => args.ascii = true,
            "--capture" => args.capture = Some(it.next().ok_or("--capture: нужен каталог")?),
            "--every" => {
                args.every = it
                    .next()
                    .ok_or("--every: нужно N")?
                    .parse()
                    .map_err(|_| "--every: не число")?
            }
            "--frames" => {
                let n: u64 = it
                    .next()
                    .ok_or("--frames: нужно N")?
                    .parse()
                    .map_err(|_| "--frames: не число")?;
                args.frames = if n == 0 { u64::MAX } else { n };
            }
            "--fps" => {
                args.fps = it
                    .next()
                    .ok_or("--fps: нужно F")?
                    .parse()
                    .map_err(|_| "--fps: не число")?
            }
            "--width" => {
                args.width = it
                    .next()
                    .ok_or("--width: нужно W")?
                    .parse()
                    .map_err(|_| "--width: не число")?
            }
            "--height" => {
                args.height = it
                    .next()
                    .ok_or("--height: нужно H")?
                    .parse()
                    .map_err(|_| "--height: не число")?
            }
            "--input" => args.input = Some(it.next().ok_or("--input: нужен файл")?),
            "--stdin-keys" => args.stdin_keys = true,
            "--fast" => args.fast = true,
            "--bench" => args.bench = true,
            "--bench-parse" => args.bench_parse = true,
            "--wgpu" => args.wgpu = true,
            "--eval" => args.eval_code = Some(it.next().ok_or("--eval: нужно выражение")?),
            "--serve" => args.serve = true,
            "--port" => {
                args.port = it
                    .next()
                    .ok_or("--port: нужно N")?
                    .parse()
                    .map_err(|_| "--port: не число")?
            }
            "--mcp" => args.mcp = true,
            "--help" | "-h" => {
                println!("{}", USAGE);
                std::process::exit(0);
            }
            other => return Err(format!("неизвестный флаг: {}", other)),
        }
    }
    Ok(args)
}

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

fn run_eval(code: &str) -> Result<(), String> {
    let halrc = Rc::new(RefCell::new(Hal::new(OutputMode::Silent, 8, 8)));
    let (ctx, cell) = make_context(halrc, PathBuf::from("."));
    let prog = parse_program(code)?;
    let mut last = Value::Nil;
    // последний результат переживает сборки на границах форм — держим pin
    let pin = gc::pin(vec![last.clone()]);
    let g = *cell.borrow();
    for e in prog {
        last = interp::eval(&e, g, 0)?;
        gc::pin_replace(&pin, vec![last.clone()]);
        gc::maybe_collect(&[g], &[]);
    }
    println!("{}", fmt_value(&last));
    drop(pin);
    let _ = ctx;
    Ok(())
}

#[cfg(feature = "jit")]
fn bench_jit() -> Result<(), String> {
    // Этап 3: бенч JIT числовых ядер. Сцена помечена честно:
    // «interp» — ПОЛНЫЙ путь интерпретатора (apply_lambda: bind_params +
    // eval каждого узла); «JIT» — apply() с хуком JIT (гард арности/типов +
    // нативный вызов скомпилированного x86-64 ядра). Это НЕ «вся игра стала
    // в 5 раз быстрее» — ускоряется вызов числовой лямбды на горячем пути.
    use std::time::Instant;

    println!("== rlisp-engine :: бенч JIT числовых ядер (Этап 3) ==");
    let halrc = Rc::new(RefCell::new(Hal::new(OutputMode::Silent, 4, 4)));
    let (ctx, cell) = make_context(halrc, PathBuf::from("assets/lisp"));
    load_file(&PathBuf::from("assets/lisp/core.lisp"), &ctx)?;
    let env = *cell.borrow();

    // ядро как в игровых update-функциях: арифметика + сравнение + if + деление
    let kernel = "(define (kern x y) \
        (if (< x y) \
            (+ (* x x) (* y 2.0)) \
            (- (* x y) (/ x y))))";
    let prog = parse_program(kernel)?;
    interp::eval(&prog[0], env, 0)?;
    let g = match interp::env_get(env, "kern") {
        Some(Value::Lambda(g)) => g,
        _ => return Err("бенч JIT: kern не лямбда".into()),
    };
    let l = gc::lambda_clone(g);
    let compiled = crate::jit::compile(&Value::Lambda(g)).is_some();
    println!("ядро: (lambda (x y) (if (< x y) (+ (* x x) (* y 2.0)) (- (* x y) (/ x y))))");
    println!(
        "JIT-компиляция: {} (фича jit, x86_64-linux)",
        if compiled {
            "успешна"
        } else {
            "деоптимизация"
        }
    );

    let n = 200_000usize;
    let mut acc = 0.0f64;

    // прогрев + интерпретатор
    let t0 = Instant::now();
    for i in 0..n {
        let x = Value::Float((i % 100) as f64 / 10.0);
        let y = Value::Float(((i * 7) % 100) as f64 / 10.0 - 5.0 + 0.05); // не 0: деление на ноль в interp — ошибка
        let r = interp::apply_lambda(&l, &[x, y], 0)?;
        if let Value::Float(f) = r {
            acc += f;
        }
    }
    let dt_interp = t0.elapsed();

    // путь с JIT (apply использует хук try_call_lambda)
    let t1 = Instant::now();
    for i in 0..n {
        let x = Value::Float((i % 100) as f64 / 10.0);
        let y = Value::Float(((i * 7) % 100) as f64 / 10.0 - 5.0 + 0.05); // не 0: деление на ноль в interp — ошибка
        let r = interp::apply(&Value::Lambda(g), &[x, y], 0)?;
        if let Value::Float(f) = r {
            acc += f;
        }
    }
    let dt_jit = t1.elapsed();

    let speedup = dt_interp.as_secs_f64() / dt_jit.as_secs_f64();
    println!(
        "[jit] interp (apply_lambda): {:>8.1} мс за {} вызовов — {:>6.1} нс/вызов",
        dt_interp.as_secs_f64() * 1e3,
        n,
        dt_interp.as_secs_f64() / n as f64 * 1e9
    );
    println!(
        "[jit] JIT (apply, гард+натив): {:>8.1} мс за {} вызовов — {:>6.1} нс/вызов",
        dt_jit.as_secs_f64() * 1e3,
        n,
        dt_jit.as_secs_f64() / n as f64 * 1e9
    );
    println!(
        "[jit] ускорение: {:.2}x (критерий этапа: ≥5x); контрольная сумма {:.4}",
        speedup, acc
    );
    Ok(())
}

fn run_bench_parse() -> Result<(), String> {
    // Этап 3: честное сравнение старого посимвольного reader'а
    // (parse_program_scalar, v0.1–v0.3) и нового SIMD-токенизатора
    // (parse_program → parse_program_simd, src/simd.rs: byte-class скан SSE2)
    // на реальном корпусе assets/lisp/*.lisp. Оба пути дают одинаковые AST —
    // сверка до замера; расхождение = ошибка, бенчмарк не продолжается.
    use std::time::Instant;

    println!("== rlisp-engine :: бенчмарк токенизатора/парсера (Этап 3) ==");
    println!("уровень SIMD: {}", simd::level_name(simd::detect()));

    let mut corpus: Vec<(String, String)> = Vec::new();
    let mut total_bytes = 0usize;
    let mut entries: Vec<_> = std::fs::read_dir("assets/lisp")
        .map_err(|e| format!("assets/lisp: {}", e))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("lisp"))
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let src = std::fs::read_to_string(e.path())
            .map_err(|err| format!("{}: {}", e.path().display(), err))?;
        total_bytes += src.len();
        corpus.push((e.file_name().to_string_lossy().into_owned(), src));
    }
    println!(
        "корпус: {} файлов, {} байт (реальные исходы движка, включая русские комментарии)",
        corpus.len(),
        total_bytes
    );

    // взаимная сверка AST (обязательна ДО замера)
    for (name, src) in &corpus {
        let a = interp::parse_program_scalar(src)?;
        let b = interp::parse_program(src)?;
        let fa: Vec<String> = a.iter().map(interp::fmt_value).collect();
        let fb: Vec<String> = b.iter().map(interp::fmt_value).collect();
        if fa != fb {
            return Err(format!("{}: AST старого и SIMD-пути расходятся!", name));
        }
    }
    println!("сверка AST scalar-vs-simd: OK ({} файлов)", corpus.len());

    // отдельный замер ТОЛЬКО лексического скана (без построения Value) —
    // показывает вклад самой векторизации, честно помечен
    let iters = 200usize;
    let bs = total_bytes as f64 * iters as f64;
    let mbs = |d: std::time::Duration| bs / d.as_secs_f64() / 1e6;
    let mut lex_scalar = 0usize;
    let mut lex_simd = 0usize;
    for (_, src) in &corpus {
        lex_scalar += interp::lex_only_scalar(src);
        lex_simd += interp::lex_only_simd(src);
    }
    if lex_scalar != lex_simd {
        return Err(format!(
            "лексический счёт расходится: {} vs {}",
            lex_scalar, lex_simd
        ));
    }

    let t2 = Instant::now();
    for _ in 0..iters {
        for (_, src) in &corpus {
            let _ = interp::lex_only_scalar(src);
        }
    }
    let dt_lex_scalar = t2.elapsed();
    let t3 = Instant::now();
    for _ in 0..iters {
        for (_, src) in &corpus {
            let _ = interp::lex_only_simd(src);
        }
    }
    let dt_lex_simd = t3.elapsed();
    let lex_speedup = dt_lex_scalar.as_secs_f64() / dt_lex_simd.as_secs_f64();
    println!(
        "[скан]   scalar (посимвольно, без Value): {:>8.1} мс — {:.0} МБ/с",
        dt_lex_scalar.as_secs_f64() * 1e3,
        mbs(dt_lex_scalar)
    );
    println!(
        "[скан]   SIMD (byte-class, без Value):    {:>8.1} мс — {:.0} МБ/с",
        dt_lex_simd.as_secs_f64() * 1e3,
        mbs(dt_lex_simd)
    );
    println!("[скан]   ускорение лексера: {:.2}x", lex_speedup);

    // прогрев (page cache, аллокаторы)
    for (_, src) in &corpus {
        let _ = interp::parse_program_scalar(src);
        let _ = interp::parse_program(src);
    }

    let t0 = Instant::now();
    let mut forms_scalar = 0usize;
    for _ in 0..iters {
        for (_, src) in &corpus {
            forms_scalar += interp::parse_program_scalar(src)?.len();
        }
    }
    let dt_scalar = t0.elapsed();

    let t1 = Instant::now();
    let mut forms_simd = 0usize;
    for _ in 0..iters {
        for (_, src) in &corpus {
            forms_simd += interp::parse_program(src)?.len();
        }
    }
    let dt_simd = t1.elapsed();

    assert_eq!(forms_scalar, forms_simd);
    let _bs = total_bytes as f64 * iters as f64;
    let speedup = dt_scalar.as_secs_f64() / dt_simd.as_secs_f64();
    println!(
        "[парсер] scalar (старый reader):  {:>8.1} мс за {} прогонов корпуса — {:.0} МБ/с",
        dt_scalar.as_secs_f64() * 1e3,
        iters,
        mbs(dt_scalar)
    );
    println!(
        "[парсер] SIMD (byte-class скан):  {:>8.1} мс за {} прогонов корпуса — {:.0} МБ/с",
        dt_simd.as_secs_f64() * 1e3,
        iters,
        mbs(dt_simd)
    );
    println!("[парсер] ускорение: {:.2}x (критерий этапа: ≥3x)", speedup);
    Ok(())
}

fn run_bench(args: &Args) -> Result<(), String> {
    println!("== rlisp-engine :: бенчмарк SIMD-слоя (критерий ТЗ §4.5) ==");
    println!("обнаружен уровень: {}", simd::level_name(simd::detect()));
    simd::bench_run();

    // Lisp-уровень: интеграция 400 «тел» (800 f32) через vec-add/vec-scale.
    let halrc = Rc::new(RefCell::new(Hal::new(OutputMode::Silent, 8, 8)));
    let (ctx, cell) = make_context(halrc, PathBuf::from("assets/lisp"));
    load_file(&PathBuf::from("assets/lisp/core.lisp"), &ctx)?;
    let code = "(begin \
        (define pos (make-f32vec 800 1.0)) \
        (define vel (make-f32vec 800 0.5)) \
        (define t0 (time-now)) \
        (dotimes (i 20000) (set! pos (vec-add pos (vec-scale vel 0.016)))) \
        (- (time-now) t0))";
    let prog = parse_program(code)?;
    let secs = interp::eval(&prog[0], *cell.borrow(), 0)?;
    let secs = match secs {
        Value::Float(f) => f,
        _ => return Err("бенчмарк: не число".into()),
    };
    let ms = secs * 1000.0;
    let mflops = 20000.0 * 800.0 * 2.0 / secs / 1e6;
    println!(
        "[lisp]  пакет: 20000 итераций vec-add+vec-scale над 800 f32 (400 тел): {:.1} мс ({:.0} млн флоп/с, {:.2} мкс/итерация)",
        ms, mflops, secs / 20000.0 * 1e6
    );

    // Контроль: та же интеграция поэлементно на самом Lisp (без SIMD-пакета).
    let code_scalar = "(begin \
        (define pos (make-f32vec 800 1.0)) \
        (define vel (make-f32vec 800 0.5)) \
        (define t0 (time-now)) \
        (dotimes (i 200) \
          (let ((j 0)) \
            (while (< j 800) \
              (f32set! pos j (+ (f32ref pos j) (* (f32ref vel j) 0.016))) \
              (set! j (+ j 1))))) \
        (- (time-now) t0))";
    let prog2 = parse_program(code_scalar)?;
    let secs2 = interp::eval(&prog2[0], *cell.borrow(), 0)?;
    let secs2 = match secs2 {
        Value::Float(f) => f,
        _ => return Err("бенчмарк: не число".into()),
    };
    let per_batch = secs / 20000.0;
    let per_scalar = secs2 / 200.0;
    println!(
        "[lisp]  скалярно на Lisp: 200 итераций поэлементного цикла: {:.0} мс ({:.1} мс/итерация)",
        secs2 * 1000.0,
        per_scalar * 1000.0
    );
    println!(
        "[lisp]  выигрыш пакета над поэлементной интерпретацией: {:.0}×",
        per_scalar / per_batch
    );

    #[cfg(feature = "jit")]
    {
        if let Err(e) = bench_jit() {
            eprintln!("[ошибка бенча JIT] {}", e);
        }
    }
    bench_2d(args)?;
    bench_3d_shading()?;
    Ok(())
}

// ——— бенчмарк 2D-кадра (этап 1): программный растр vs wgpu за фасадом HAL ———

/// Один кадр синтетической сцены 320×240. Формула ОДНА для софта, wgpu и
/// Lisp-уровня — сравнение честное: clear + 120 fill-rect + 12 blit(24×24) +
/// 4 blit-tex(2×) + present. Возвращает (мс/кадр всего, submit, readback).
fn bench_frame_ops(hal: &mut Hal, sprite: &[u8], frames: usize) -> (f64, f64, f64) {
    let mut sub = 0u128;
    let mut rb = 0u128;
    let t0 = Instant::now();
    for _ in 0..frames {
        hal.clear_all(8, 8, 18);
        for i in 0..120i64 {
            hal.fill_rect(i * 2, i * 2, 40, 20, 204, 76, 25);
        }
        for i in 0..12i64 {
            let _ = hal.blit(i * 18, i * 15, 24, 24, sprite);
        }
        for i in 0..4i64 {
            let _ = hal.blit_tex(1, i * 40, i * 40, 2.0, [1.0, 1.0, 1.0]);
        }
        let (s, r) = hal.flush_gpu();
        sub += s;
        rb += r;
    }
    let f = frames as f64;
    (
        t0.elapsed().as_secs_f64() * 1000.0 / f,
        sub as f64 / 1e6 / f,
        rb as f64 / 1e6 / f,
    )
}

/// Тот же кадр через интерпретатор: (bench-frame) на Lisp.
/// Возвращает мс/кадр; GPU-времена лежат в hal.gpu_*_ns (дельта).
fn bench_lisp_scene(halrc: Rc<RefCell<Hal>>, frames: usize) -> Result<f64, String> {
    let (ctx, cell) = make_context(halrc, PathBuf::from("assets/lisp"));
    load_file(&PathBuf::from("assets/lisp/core.lisp"), &ctx)?;
    // спрайт — глобальный вектор (Lisp-цикл не собирает его каждый кадр)
    let sprite = Value::Vector(Rc::new(RefCell::new(
        (0..24 * 24 * 4)
            .map(|i| Value::Int(if i % 4 == 3 { 255 } else { 200 }))
            .collect(),
    )));
    env_define(*cell.borrow(), "bench-sprite", sprite);
    let code = format!(
        "(begin \
         (define (bench-frame) \
           (gpu-clear 0.03 0.03 0.07) \
           (dotimes (i 120) (gpu-fill-rect (* i 2) (* i 2) 40 20 0.8 0.3 0.1)) \
           (dotimes (i 12) (gpu-blit (* i 18) (* i 15) 24 24 bench-sprite)) \
           (dotimes (i 4) (gpu-blit-tex 1 (* i 40) (* i 40) 2.0 (list 1.0 1.0 1.0))) \
           (gpu-present)) \
         (define t0 (time-now)) \
         (dotimes (i {}) (bench-frame)) \
         (- (time-now) t0))",
        frames
    );
    let prog = parse_program(&code)?;
    let secs = interp::eval(&prog[0], *cell.borrow(), 0)?;
    match secs {
        Value::Float(f) => Ok(f * 1000.0 / frames as f64),
        _ => Err("бенчмарк 2D: Lisp вернул не число".into()),
    }
}

fn bench_tex() -> Vec<u8> {
    (0..32 * 32)
        .flat_map(|i| {
            [
                (i % 251) as u8,
                ((i * 7) % 255) as u8,
                ((i * 13) % 255) as u8,
                255u8,
            ]
        })
        .collect()
}

fn bench_sprite() -> Vec<u8> {
    (0..24 * 24)
        .flat_map(|i| {
            [
                (i % 200) as u8,
                128,
                ((i * 3) % 200) as u8,
                if i % 16 == 0 { 0u8 } else { 255u8 }, // чуть прозрачности для blit
            ]
        })
        .collect()
}

fn bench_2d(args: &Args) -> Result<(), String> {
    println!();
    println!("== rlisp-engine :: бенчмарк 2D-кадра 320×240 (этап 1) ==");
    println!(
        "сцена (одинаковая для всех бэкендов): gpu-clear + 120 gpu-fill-rect + 12 gpu-blit 24×24 + 4 gpu-blit-tex ×2 + gpu-present"
    );
    let (sprite, tex) = (bench_sprite(), bench_tex());
    const FRAMES_RUST: usize = 300;
    const FRAMES_LISP: usize = 100;

    // — программный растр (дефолт, ноль зависимостей) —
    let mut hal = Hal::new(OutputMode::Silent, 320, 240);
    hal.tex.push(Tex {
        w: 32,
        h: 32,
        px: tex.clone(),
    });
    hal.tex_register(1, 32, 32, &tex);
    let (t, _s, _r) = bench_frame_ops(&mut hal, &sprite, FRAMES_RUST);
    println!(
        "[soft] Rust HAL, прямые вызовы (чистая стоимость растра без Lisp): {:.2} мс/кадр ({} кадров)",
        t, FRAMES_RUST
    );
    let halrc = Rc::new(RefCell::new(Hal::new(OutputMode::Silent, 320, 240)));
    {
        let mut h = halrc.borrow_mut();
        h.tex.push(Tex {
            w: 32,
            h: 32,
            px: tex.clone(),
        });
        h.tex_register(1, 32, 32, &tex);
    }
    let lisp_soft = bench_lisp_scene(halrc, FRAMES_LISP)?;
    println!(
        "[soft] Lisp поверх HAL (тот же кадр через интерпретатор): {:.2} мс/кадр ({} кадров) — накладные расходы Lisp: {:.2} мс/кадр",
        lisp_soft,
        FRAMES_LISP,
        (lisp_soft - t).max(0.0)
    );

    // — wgpu-бэкенд (только по флагу --wgpu и при наличии адаптера) —
    if !args.wgpu {
        println!("[wgpu] пропущено: запустите `--bench --wgpu` (нужны фича cargo --features wgpu и GPU-адаптер)");
        return Ok(());
    }
    let gpu_backend = match gpu::create_wgpu_backend(320, 240) {
        Ok(b) => b,
        Err(e) => {
            println!("wgpu-адаптер недоступен ({}): wgpu-бенчмарк пропущен", e);
            return Ok(());
        }
    };
    let mut hal = Hal::with_backend(
        OutputMode::Silent,
        320,
        240,
        gpu::Backend::Wgpu(gpu_backend),
    );
    hal.tex.push(Tex {
        w: 32,
        h: 32,
        px: tex.clone(),
    });
    hal.tex_register(1, 32, 32, &tex);
    let (t, s, r) = bench_frame_ops(&mut hal, &sprite, FRAMES_RUST);
    println!(
        "[wgpu] Rust HAL, прямые вызовы: {:.2} мс/кадр ({} кадров) = submit {:.2} (запись+отправка команд GPU) + readback {:.2} (copy+map+композит CPU←GPU)",
        t, FRAMES_RUST, s, r
    );
    let halrc = Rc::new(RefCell::new(Hal::with_backend(
        OutputMode::Silent,
        320,
        240,
        gpu::Backend::Wgpu(gpu::create_wgpu_backend(320, 240)?),
    )));
    {
        let mut h = halrc.borrow_mut();
        h.tex.push(Tex {
            w: 32,
            h: 32,
            px: tex.clone(),
        });
        h.tex_register(1, 32, 32, &tex);
    }
    let lisp_gpu = bench_lisp_scene(halrc, FRAMES_LISP)?;
    println!(
        "[wgpu] Lisp поверх HAL (тот же кадр через интерпретатор): {:.2} мс/кадр ({} кадров)",
        lisp_gpu, FRAMES_LISP
    );
    println!();
    println!("честная метка этапа 1: сравнение «wgpu кратно быстрее» требует GPU-железа;");
    println!("в headless/CI без адаптера wgpu-путь откатывается на программный растр.");
    Ok(())
}

// ——— бенчмарк 3D-шейдинга (этап 7): стоимость материалов в программном
// конвейере. Сцена ОДНА для всех конфигураций — дельта честная ———

struct Сцена3d {
    pos: Vec<f32>,
    uv: Vec<f32>,
    idx: Vec<f32>,
    insts: Vec<f32>,
    mvp: [f32; 16],
}

fn mat_lookat(eye: [f32; 3], target: [f32; 3]) -> [f32; 16] {
    let f = {
        let d = [target[0] - eye[0], target[1] - eye[1], target[2] - eye[2]];
        let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-9);
        [d[0] / l, d[1] / l, d[2] / l]
    };
    let up = [0.0f32, 1.0, 0.0];
    let r = {
        let c = [
            f[1] * up[2] - f[2] * up[1],
            f[2] * up[0] - f[0] * up[2],
            f[0] * up[1] - f[1] * up[0],
        ];
        let l = (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt().max(1e-9);
        [c[0] / l, c[1] / l, c[2] / l]
    };
    let u = [
        r[1] * f[2] - r[2] * f[1],
        r[2] * f[0] - r[0] * f[2],
        r[0] * f[1] - r[1] * f[0],
    ];
    let d = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    [
        r[0],
        r[1],
        r[2],
        -d(r, eye),
        u[0],
        u[1],
        u[2],
        -d(u, eye),
        -f[0],
        -f[1],
        -f[2],
        d(f, eye),
        0.0,
        0.0,
        0.0,
        1.0,
    ]
}

fn mat_persp(fov: f32, aspect: f32, near: f32, far: f32) -> [f32; 16] {
    let f = 1.0 / (fov * 0.5).tan();
    [
        f / aspect,
        0.0,
        0.0,
        0.0,
        0.0,
        f,
        0.0,
        0.0,
        0.0,
        0.0,
        (far + near) / (near - far),
        2.0 * far * near / (near - far),
        0.0,
        0.0,
        -1.0,
        0.0,
    ]
}

/// o = a·b (row-major, вектор-столбец — конвенция render3d.lisp)
fn mat_mul(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut o = [0.0f32; 16];
    for r in 0..4 {
        for c in 0..4 {
            let mut s = 0.0;
            for k in 0..4 {
                s += a[r * 4 + k] * b[k * 4 + c];
            }
            o[r * 4 + c] = s;
        }
    }
    o
}

fn bench_3d_shading() -> Result<(), String> {
    println!();
    println!("== rlisp-engine :: бенчмарк 3D-шейдинга (этап 7, программный конвейер) ==");
    println!("сцена (одинаковая для всех конфигураций): батч-вершинник, 100 текстурных квадов 6×6, экран 320×240");
    const FRAMES: usize = 100;

    // текстура: «трава» 32×32 с альфа-полосами (alpha-test вырезает 25% текселей)
    let mut px = vec![0u8; 32 * 32 * 4];
    for y in 0..32usize {
        for x in 0..32usize {
            let i = (y * 32 + x) * 4;
            px[i] = 40;
            px[i + 1] = 110 + ((x * 7 + y * 13) % 6) as u8 * 14;
            px[i + 2] = 30;
            px[i + 3] = if (x + y) % 8 < 2 { 0 } else { 255 };
        }
    }

    // квад-билборд в плоскости XY (лицом к камере на +Z)
    let scene = Сцена3d {
        pos: vec![
            -1.0, -1.0, 0.0, 1.0, -1.0, 0.0, 1.0, 1.0, 0.0, -1.0, 1.0, 0.0,
        ],
        uv: vec![0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0],
        idx: vec![0.0, 1.0, 2.0, 0.0, 2.0, 3.0],
        insts: {
            let mut v = Vec::with_capacity(100 * 12);
            for gy in 0..10 {
                for gx in 0..10 {
                    let x = (gx as f32 - 4.5) * 5.0;
                    let y = (gy as f32 - 4.5) * 3.8 + 1.0;
                    let z = -(gy as f32) * 0.8;
                    v.extend_from_slice(&[x, y, z, 6.0, 6.0, 6.0, 0.0, 0.0, 0.0, 0.55, 0.85, 0.4]);
                }
            }
            v
        },
        mvp: {
            let eye = [0.0f32, 2.0, 28.0];
            let view = mat_lookat(eye, [0.0, 0.0, 0.0]);
            let proj = mat_persp(0.9, 320.0 / 240.0, 0.5, 400.0);
            mat_mul(&proj, &view)
        },
    };

    let mut hal = Hal::new(OutputMode::Silent, 320, 240);
    hal.tex.push(Tex {
        w: 32,
        h: 32,
        px: px.clone(),
    });
    hal.tex_register(1, 32, 32, &px);
    // свет и камера — как в игре: направление к источнику, view для блика
    hal.r3.light = [0.3, 0.7, 0.65];
    hal.r3.ambient = 0.35;
    hal.r3.diffuse = 0.75;
    hal.r3.spec = 0.0;
    hal.r3.shininess = 32.0;
    hal.r3.view = [0.0, 2.0, 28.0];
    hal.r3.alpha_test = 0.0;
    hal.r3.tiling = [1.0, 1.0];
    hal.r3.fog = [0.6, 0.75, 0.9, 0.0];

    let кадр = |hal: &mut Hal, s: &Сцена3d| -> Result<u64, String> {
        hal.clear_all(8, 8, 18);
        // как (gpu3d-clear): очистка z-буфера и статистики на каждый кадр
        hal.r3.zbuf.fill(f32::INFINITY);
        hal.r3.drawn = 0;
        hal.r3.culled = 0;
        render3d::batch_draw(hal, &s.pos, Some(&s.uv), &s.idx, &s.insts, &s.mvp, 1)
    };
    let замер = |hal: &mut Hal, метка: &str, s: &Сцена3d| -> Result<f64, String> {
        // прогрев без замера (прогрев кэша/веток)
        кадр(hal, s)?;
        let t0 = Instant::now();
        let mut drawn = 0u64;
        for _ in 0..FRAMES {
            drawn = кадр(hal, s)?;
        }
        let ms = t0.elapsed().as_secs_f64() * 1000.0 / FRAMES as f64;
        if drawn == 0 {
            return Err("бенч 3D: сцена не рисует ни одного треугольника (куллинг?)".into());
        }
        println!("[3d] {:<44} {:.2} мс/кадр ({} кадров)", метка, ms, FRAMES);
        Ok(ms)
    };

    let base = замер(&mut hal, "Lambert, материалы выкл (база этапа 6)", &scene)?;
    hal.r3.alpha_test = 0.5;
    let at = замер(&mut hal, "+ alpha-test 0.5 (cutout 25% текселей)", &scene)?;
    hal.r3.alpha_test = 0.0;
    hal.r3.spec = 0.6;
    let sp = замер(
        &mut hal,
        "+ блик Blinn-Phong (spec 0.6, shininess 32)",
        &scene,
    )?;
    hal.r3.spec = 0.0;
    hal.r3.fog = [0.6, 0.75, 0.9, 0.06];
    let fg = замер(&mut hal, "+ туман (density 0.06, глубина 1/w)", &scene)?;
    hal.r3.alpha_test = 0.5;
    hal.r3.spec = 0.6;
    let all = замер(
        &mut hal,
        "весь материал (alpha-test + блик + туман)",
        &scene,
    )?;

    let pct = |new: f64, old: f64| (new / old - 1.0) * 100.0;
    println!(
        "дельта к базе: alpha-test {:+.1}%, блик {:+.1}%, туман {:+.1}%, весь материал {:+.1}%",
        pct(at, base),
        pct(sp, base),
        pct(fg, base),
        pct(all, base)
    );
    println!(
        "честная метка: программный конвейер на CPU VPS-класса; разброс между запусками ±20% (см. docs/benchmark.md)"
    );
    Ok(())
}

fn parse_input_file(path: &str) -> Result<Vec<(f64, Value)>, String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("input {}: {}", path, e))?;
    let mut out = vec![];
    for (ln, line) in src.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() != 3 {
            return Err(format!(
                "input {}: строка {}: ожидается «ВРЕМЯ КЛАВИША down|up»",
                path,
                ln + 1
            ));
        }
        let t: f64 = parts[0]
            .parse()
            .map_err(|_| format!("input {}: строка {}: плохое время", path, ln + 1))?;
        let ev = if parts[2] == "down" {
            "key-down"
        } else {
            "key-up"
        };
        out.push((
            t,
            Value::List(vec![
                Value::Sym(ev.into()),
                Value::Str(parts[1].to_string()),
            ]),
        ));
    }
    out.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    Ok(out)
}

fn spawn_stdin() -> std::sync::mpsc::Receiver<u8> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = [0u8; 64];
        let stdin = std::io::stdin();
        let mut lock = stdin.lock();
        loop {
            match lock.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    for &b in &buf[..n] {
                        if tx.send(b).is_err() {
                            return;
                        }
                    }
                }
                Err(_) => break,
            }
        }
    });
    rx
}

pub fn snapshot_mtimes(files: &[PathBuf]) -> HashMap<PathBuf, SystemTime> {
    let mut m = HashMap::new();
    for p in files {
        if let Ok(mt) = std::fs::metadata(p).and_then(|md| md.modified()) {
            m.insert(p.clone(), mt);
        }
    }
    m
}

/// Hot reload по ТЗ §8: новое Lisp-окружение, перечитанные файлы,
/// состояние из defpersistent переносится из старого мира в новый.
pub fn reload_all(
    ctx: &CoreCtx,
    halrc: &Rc<RefCell<Hal>>,
    mtimes: &mut HashMap<PathBuf, SystemTime>,
) -> Result<usize, String> {
    let g_old = *ctx.global.borrow();
    let mut saved: Vec<(String, Value)> = vec![];
    if let Some(Value::Table(t)) = env_get(g_old, "*persistent*") {
        let keys: Vec<String> = t.borrow().keys().cloned().collect();
        for k in keys {
            if let Some(v) = env_get(g_old, &k) {
                saved.push((k, v));
            }
        }
    }
    let n_saved = saved.len();
    // persistent-значения переживают сборки во время load_file — pin-корни
    let _pin = gc::pin(saved.iter().map(|(_, v)| v.clone()).collect());
    // Этап 1 (менеджер GPU-ресурсов): старые текстуры, на которые не ссылается
    // persistent-состояние, освобождаются при reload — раньше они оставались в
    // реестре до рестарта (утечка ~64 КБ/текстуру за reload).
    let mut keep: Vec<i64> = vec![];
    for (_, v) in &saved {
        gpu::collect_int_refs(v, &mut keep, 0);
    }
    let invalidated = halrc.borrow_mut().invalidate_textures_except(&keep);
    if invalidated > 0 {
        eprintln!(
            "[hot-reload] GPU-ресурсы: освобождено текстур: {}",
            invalidated
        );
    }
    let new_env = env_new(None);
    *ctx.global.borrow_mut() = new_env;
    register_all(new_env, ctx, halrc);
    env_define(
        new_env,
        "*root*",
        Value::Str(ctx.root.to_string_lossy().into_owned()),
    );
    let files: Vec<PathBuf> = ctx.registry.borrow().clone();
    for p in &files {
        load_file(p, ctx)?;
    }
    for (k, v) in saved {
        if env_get(new_env, &k).is_some() {
            env_define(new_env, &k, v);
        }
    }
    // граница reload: старый мир (окружение/замыкания) собирается здесь
    gc::maybe_collect(&[*ctx.global.borrow()], &[]);
    *mtimes = snapshot_mtimes(&files);
    Ok(n_saved)
}

fn run_engine(args: &Args) -> Result<(), String> {
    let script = PathBuf::from(&args.script);
    let root = script
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let out_mode = if let Some(dir) = &args.capture {
        std::fs::create_dir_all(dir).map_err(|e| format!("capture {}: {}", dir, e))?;
        OutputMode::Png {
            dir: PathBuf::from(dir),
            every: args.every.max(1),
        }
    } else if args.ascii {
        OutputMode::Ascii
    } else {
        OutputMode::Silent
    };

    // — бэкенд рендера: --wgpu → wgpu за фасадом HAL; без адаптера/фичи —
    //   честный откат на программный растр (все скрипты работают как раньше) —
    let (backend, render_note) = if args.wgpu {
        let probe = gpu::create_wgpu_backend(args.width, args.height);
        if let Some(w) = gpu::fallback_warning(&probe) {
            eprintln!("{}", w);
        }
        match probe {
            Ok(b) => {
                let info = b.adapter_info();
                (gpu::Backend::Wgpu(b), format!("wgpu: {}", info))
            }
            Err(_) => (
                gpu::Backend::Software,
                "программный растр (fallback из --wgpu)".into(),
            ),
        }
    } else {
        (gpu::Backend::Software, "программный растр".into())
    };
    let halrc = Rc::new(RefCell::new(Hal::with_backend(
        out_mode,
        args.width,
        args.height,
        backend,
    )));
    let (ctx, cell) = make_context(halrc.clone(), root.clone());
    env_define(
        *cell.borrow(),
        "*root*",
        Value::Str(root.to_string_lossy().into_owned()),
    );

    load_file(&script, &ctx)?;
    let (w, h) = {
        let hb = halrc.borrow();
        (hb.w, hb.h)
    };

    // --width/--height — авторитет CLI: window-create сцены мог задать свой
    // размер (константы graphics.lisp) — возвращаем размер из аргументов и
    // синхронизируем *screen-w*/*screen-h*, чтобы HUD/небо сцены строились
    // под реальный фреймбуфер (раньше --width менял только буфер HAL).
    if (w, h) != (args.width, args.height) {
        halrc.borrow_mut().resize_window(args.width, args.height);
    }
    if args.width != 320 || args.height != 240 {
        let g = *cell.borrow();
        for (name, val) in [("*screen-w*", args.width), ("*screen-h*", args.height)] {
            let code = format!("(set! {} {})", name, val);
            for form in parse_program(&code)? {
                interp::eval(&form, g, 0)?;
            }
        }
    }

    // v0.3: мост редактора (--serve) — публикуем кадр/сцену, принимаем команды
    let bridge = if args.serve {
        let b = editor::Bridge::new();
        b.set_audio(halrc.borrow().audio.clone());
        editor::install_bridge(b.clone());
        editor::start_server(b.clone(), args.port, root.clone())
            .map_err(|e| format!("редактор: порт {} занят: {}", args.port, e))?;
        println!(
            "редактор: http://127.0.0.1:{} (иерархия · инспектор · REPL · ассеты)",
            args.port
        );
        let assets: Vec<String> = ctx
            .registry
            .borrow()
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        *b.assets.lock().unwrap() = assets;
        Some(b)
    } else {
        None
    };

    let events = match &args.input {
        Some(p) => parse_input_file(p)?,
        None => vec![],
    };
    let files: Vec<PathBuf> = ctx.registry.borrow().clone();
    let mut mtimes = snapshot_mtimes(&files);
    let n_files = files.len();

    println!(
        "rlisp-engine v0.14 | SIMD: {} | рендер: {} | экран {}x{} | lisp-файлов: {} | скрипт: {}",
        simd::level_name(simd::detect()),
        render_note,
        w,
        h,
        n_files,
        args.script
    );
    if args.ascii {
        println!("управление: wasd — движение, q — выход");
        print!("\x1b[2J");
        std::io::stdout().flush().ok();
    }

    let rx = if args.stdin_keys {
        let _ = Command::new("stty").arg("raw").arg("-echo").status();
        Some(spawn_stdin())
    } else {
        None
    };

    let fixed_dt = 1.0 / args.fps;
    let use_fixed = args.fast || args.capture.is_some() || args.input.is_some();
    let mut sim_time = 0f64;
    let mut ev_i = 0usize;
    let mut frames: u64 = 0;
    let mut reloads: u64 = 0;
    let mut quit = false;
    let wall_start = Instant::now();
    let mut last_frame_at = Instant::now();
    let mut next_frame = wall_start + Duration::from_secs_f64(fixed_dt);
    // профилировка кадра: RLISP_PROFILE=1 → разбивка update/draw/frame-end/
    // gc/прочее (мс на кадр, среднее за 120 кадров)
    let prof = std::env::var_os("RLISP_PROFILE").is_some();
    let mut t_upd = Duration::ZERO;
    let mut t_draw = Duration::ZERO;
    let mut t_fe = Duration::ZERO;
    let mut t_gc = Duration::ZERO;
    let mut t_all = Duration::ZERO;
    let mut t_frame = Instant::now();

    while frames < args.frames {
        // ввод из сценария
        while ev_i < events.len() && events[ev_i].0 <= sim_time {
            halrc.borrow_mut().queue.push(events[ev_i].1.clone());
            ev_i += 1;
        }
        // ввод с клавиатуры
        if let Some(rx) = &rx {
            while let Ok(b) = rx.try_recv() {
                match b {
                    b'q' | b'Q' | 3 => quit = true,
                    b'a'..=b'z' => {
                        let k = (b as char).to_string();
                        halrc.borrow_mut().queue.push(Value::List(vec![
                            Value::Sym("key-down".into()),
                            Value::Str(k),
                        ]));
                    }
                    b'A'..=b'Z' => {
                        let k = (b as char).to_lowercase().to_string();
                        halrc.borrow_mut().queue.push(Value::List(vec![
                            Value::Sym("key-up".into()),
                            Value::Str(k),
                        ]));
                    }
                    _ => {}
                }
            }
        }
        if quit {
            break;
        }

        // v0.3: команды редактора (eval/keys) + ручной reload
        let mut editor_skip = false;
        if let Some(b) = &bridge {
            if editor::drain_editor_cmds(b, &cell, &halrc) {
                match reload_all(&ctx, &halrc, &mut mtimes) {
                    Ok(n) => {
                        reloads += 1;
                        b.log_line(format!("[reload] {} persistent-значений восстановлено", n));
                    }
                    Err(e) => b.log_line(format!("[reload] ошибка: {}", e)),
                }
                let assets: Vec<String> = ctx
                    .registry
                    .borrow()
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect();
                *b.assets.lock().unwrap() = assets;
            }
            let paused = b.paused.load(std::sync::atomic::Ordering::Relaxed);
            let step = b.step_req.swap(false, std::sync::atomic::Ordering::Relaxed);
            editor_skip = paused && !step;
        }

        // hot reload: следим за .lisp-файлами каждый кадр
        let mut changed: Option<PathBuf> = None;
        for p in &files {
            if let Ok(mt) = std::fs::metadata(p).and_then(|md| md.modified()) {
                if mtimes.get(p) != Some(&mt) {
                    changed = Some(p.clone());
                    break;
                }
            }
        }
        if let Some(p) = changed {
            match reload_all(&ctx, &halrc, &mut mtimes) {
                Ok(n) => {
                    reloads += 1;
                    eprintln!(
                        "[hot-reload] кадр {}: {} перезагружен; окружение пересобрано, {} persistent-значений восстановлено",
                        frames, p.display(), n
                    );
                }
                Err(e) => eprintln!(
                    "[hot-reload] ошибка: {} — продолжаем на старом окружении",
                    e
                ),
            }
        }

        // dt
        let dt = if use_fixed {
            fixed_dt
        } else {
            let d = last_frame_at.elapsed().as_secs_f64().clamp(0.001, 0.05);
            last_frame_at = Instant::now();
            d
        };

        // кадр: update(dt) → draw() → frame-end() (в паузе редактора — пропуск)
        // RLISP_PROFILE=1 — накопление разбивки времени кадра (update/draw/
        // frame-end/GC) и печать каждые 120 кадров + в итоге
        if !editor_skip {
            let g = *cell.borrow();
            let mut err: Option<String> = None;
            let t0 = Instant::now();
            if let Some(f) = env_get(g, "update") {
                if let Err(e) = apply(&f, &[Value::Float(dt)], 0) {
                    err = Some(format!("update: {}", e));
                }
            }
            if prof {
                t_upd += t0.elapsed();
            }
            let t0 = Instant::now();
            if err.is_none() {
                if let Some(f) = env_get(g, "draw") {
                    if let Err(e) = apply(&f, &[], 0) {
                        err = Some(format!("draw: {}", e));
                    }
                }
            }
            if prof {
                t_draw += t0.elapsed();
            }
            let t0 = Instant::now();
            if err.is_none() {
                if let Some(f) = env_get(g, "frame-end") {
                    let _ = apply(&f, &[], 0);
                }
            }
            if prof {
                t_fe += t0.elapsed();
            }
            if let Some(e) = err {
                return Err(format!("кадр {}: {}", frames, e));
            }
        }

        // Этап 2: граница форм — авто-сборка GC. Очередь ввода (Value-события)
        // передаём как дополнительные корни: в будущем события могут нести пары.
        let queue_roots = halrc.borrow().queue.clone();
        let t0 = Instant::now();
        gc::maybe_collect(&[*cell.borrow()], &queue_roots);
        if prof {
            t_gc += t0.elapsed();
        }

        if prof {
            t_all += t_frame.elapsed();
            t_frame = Instant::now();
            if frames % 120 == 119 {
                let k = 120.0 / (t_all.as_secs_f64() + 1e-9);
                eprintln!(
                    "[profile] update {:.1} | draw {:.1} | frame-end {:.1} | gc {:.1} | прочее {:.1} мс/кадр ({} fps)",
                    t_upd.as_secs_f64() * 1e3 / 120.0,
                    t_draw.as_secs_f64() * 1e3 / 120.0,
                    t_fe.as_secs_f64() * 1e3 / 120.0,
                    t_gc.as_secs_f64() * 1e3 / 120.0,
                    (t_all - t_upd - t_draw - t_fe - t_gc).as_secs_f64() * 1e3 / 120.0,
                    (k as u32)
                );
                t_all = Duration::ZERO;
                (t_upd, t_draw, t_fe, t_gc) = (
                    Duration::ZERO,
                    Duration::ZERO,
                    Duration::ZERO,
                    Duration::ZERO,
                );
            }
        }

        frames += 1;
        sim_time += dt;

        // v0.3: публикация кадра/сцены/статистики в мост редактора
        if let Some(b) = &bridge {
            {
                let mut hal = halrc.borrow_mut();
                // wgpu-режим: живой вид редактора читает fb — досылаем GPU-слой
                let _ = hal.flush_gpu();
                *b.fb.lock().unwrap() = hal.fb.clone();
                *b.fb_wh.lock().unwrap() = (hal.w, hal.h);
                b.drawcalls
                    .store(hal.r3.drawn, std::sync::atomic::Ordering::Relaxed);
            }
            b.frame.store(frames, std::sync::atomic::Ordering::Relaxed);
            // fps считаем только по реально отрисованным кадрам: в паузе
            // редактора цикл крутится вхолостую и «fps 12000» — ложь
            if !editor_skip {
                let inst = 1.0 / last_frame_at.elapsed().as_secs_f64().max(1e-6);
                let mut f = b.fps.lock().unwrap();
                *f = if *f <= 0.0 {
                    inst
                } else {
                    *f * 0.9 + inst * 0.1
                };
            }
            editor::publish_scene(&cell, b);
            editor::publish_extras(&cell, b);
        }

        // удержание реального времени (для demo hot reload по настенным часам)
        if !args.fast {
            next_frame += Duration::from_secs_f64(fixed_dt);
            let now = Instant::now();
            if next_frame > now {
                std::thread::sleep(next_frame - now);
            } else {
                next_frame = now;
            }
        }
    }

    if args.stdin_keys {
        let _ = Command::new("stty").arg("sane").status();
    }

    let g = *cell.borrow();
    let score = env_get(g, "*score*")
        .map(|v| fmt_value(&v))
        .unwrap_or_else(|| "?".into());
    let wall = wall_start.elapsed().as_secs_f64();
    println!(
        "итог: кадров {} за {:.1}с ({:.1} fps) | hot-reload: {} | score: {}",
        frames,
        wall,
        frames as f64 / wall.max(1e-9),
        reloads,
        score
    );
    Ok(())
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{}\n\n{}", e, USAGE);
            std::process::exit(2);
        }
    };
    // калибровка SIMD-уровня по фактической скорости (см. simd.rs)
    let lvl = simd::calibrate();
    if std::env::args().any(|a| a == "--bench") {
        println!("уровень после калибровки: {}", simd::level_name(lvl));
    }
    if args.bench {
        if let Err(e) = run_bench(&args) {
            eprintln!("[ошибка] {}", e);
            std::process::exit(1);
        }
        return;
    }
    if args.bench_parse {
        if let Err(e) = run_bench_parse() {
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
        if let Err(e) = run_eval(&code) {
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
            if let Err(e) = run_engine(&args) {
                eprintln!("[ошибка] {}", e);
                std::process::exit(1);
            }
        })
        .unwrap();
    if handler.join().is_err() {
        std::process::exit(1);
    }
}
