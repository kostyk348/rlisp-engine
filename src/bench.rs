// src/bench.rs — бенчмарки SIMD, JIT, токенизатора, 2D- и 3D-шейдинга.
// Запускаются по флагам --bench и --bench-parse.

use crate::cli::Args;
#[cfg(feature = "count-allocs")]
use alloc_counter::Counting;
#[cfg(feature = "jit")]
use crate::gc;
use crate::hal::{Hal, OutputMode, Tex};
use crate::interp::{self, env_define, load_file, parse_program, Value};
use crate::make_context;
use crate::{gpu, render3d, simd};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

#[cfg(feature = "jit")]
fn bench_jit() -> Result<(), String> {
    // Этап 3: бенч JIT числовых ядер. Сцена помечена честно:
    // «interp» — ПОЛНЫЙ путь интерпретатора (apply_lambda: bind_params +
    // eval каждого узла); «JIT» — apply() с хуком JIT (гард арности/типов +
    // нативный вызов скомпилированного x86-64 ядра). Это НЕ «вся игра стала
    // в 5 раз быстрее» — ускоряется вызов числовой лямбды на горячем пути.
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

pub fn run_bench_parse() -> Result<(), String> {
    // Этап 3: честное сравнение старого посимвольного reader'а
    // (parse_program_scalar, v0.1–v0.3) и нового SIMD-токенизатора
    // (parse_program → parse_program_simd, src/simd.rs: byte-class скан SSE2)
    // на реальном корпусе assets/lisp/*.lisp. Оба пути дают одинаковые AST —
    // сверка до замера; расхождение = ошибка, бенчмарк не продолжается.
    println!("== rlisp-engine :: бенчмарк токенизатора/парсера (Этап 3) ==");
    println!("уровень SIMD: {}", simd::level_name(simd::detect()));

    let (corpus, total_bytes) = load_corpus()?;
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

pub fn run_bench(args: &Args) -> Result<(), String> {
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
        let (s, r) = hal.flush_gpu(true);
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
    hal.tex.push(Tex::new(32, 32, tex.clone()));
    hal.tex_register(1, 32, 32, &tex);
    let (t, _s, _r) = bench_frame_ops(&mut hal, &sprite, FRAMES_RUST);
    println!(
        "[soft] Rust HAL, прямые вызовы (чистая стоимость растра без Lisp): {:.2} мс/кадр ({} кадров)",
        t, FRAMES_RUST
    );
    let halrc = Rc::new(RefCell::new(Hal::new(OutputMode::Silent, 320, 240)));
    {
        let mut h = halrc.borrow_mut();
        h.tex.push(Tex::new(32, 32, tex.clone()));
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
    hal.tex.push(Tex::new(32, 32, tex.clone()));
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
        h.tex.push(Tex::new(32, 32, tex.clone()));
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
    hal.tex.push(Tex::new(32, 32, px.clone()));
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
        render3d::batch_draw(hal, &s.pos, Some(&s.uv), None, &s.idx, &s.insts, &s.mvp, 1)
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

/// Корпус бенча: `assets/lisp/*.lisp` — реальные исходы движка.
fn load_corpus() -> Result<(Vec<(String, String)>, usize), String> {
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
    Ok((corpus, total_bytes))
}

/// Сколько лексических токенов потребил бы парсер из этого исхода: скобки, кавычки,
/// атомы (включая числа), строки как один токен. Комментарии — не токены: парсер их
/// пропускает, не создавая Value, поэтому считать их токенами значит занизить метрику.
fn count_tokens(bytes: &[u8]) -> usize {
    let is_delim = |b: u8| {
        matches!(b, b'(' | b')' | b'[' | b']' | b'\'' | b'`' | b',' | b';' | b'"')
    };
    let mut i = 0usize;
    let mut tokens = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b';' => {
                i = simd::find_byte(bytes, i + 1, b'\n').unwrap_or(bytes.len());
            }
            b'(' | b')' | b'[' | b']' | b'\'' | b'`' | b',' => {
                tokens += 1;
                i += 1;
            }
            b'"' => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 2;
                    } else {
                        let closed = bytes[i] == b'"';
                        i += 1;
                        if closed {
                            break;
                        }
                    }
                }
                tokens += 1;
            }
            b if b.is_ascii_whitespace() => i += 1,
            _ => {
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() && !is_delim(bytes[i]) {
                    i += 1;
                }
                tokens += 1;
            }
        }
    }
    tokens
}

/// Критерий ТЗ (`rlisp-engine-tz.md` §2.1): аллокаций на ASCII-файл ≤ 0,5/токен.
/// Сборка: `cargo run --release --features count-allocs -- --bench-parse-allocs`.
pub fn run_bench_parse_allocs() -> Result<(), String> {
    #[cfg(not(feature = "count-allocs"))]
    {
        return Err("соберите с фичей счётчика: cargo run --release --features count-allocs -- --bench-parse-allocs".into());
    }
    #[cfg(feature = "count-allocs")]
    {
        let (corpus, total_bytes) = load_corpus()?;
        println!("== rlisp-engine :: аллокации парсера (критерий ≤ 0,5/токен) ==");
        println!("корпус: {} файлов, {} байт", corpus.len(), total_bytes);
        let mut allocs_total = 0u64;
        let mut tokens_total = 0usize;
        for (name, src) in &corpus {
            Counting::reset();
            let _values = interp::parse_program(src)?;
            let tokens = count_tokens(src.as_bytes());
            let allocs = Counting::count();
            allocs_total += allocs;
            tokens_total += tokens;
            let per = allocs as f64 / tokens.max(1) as f64;
            println!("  {:<28} токенов {:>6}  аллокаций {:>6}  = {:.3}/токен", name, tokens, allocs, per);
        }
        let per_total = allocs_total as f64 / tokens_total.max(1) as f64;
        println!(
            "итого: {} токенов, {} аллокаций = {:.3}/токен — {}",
            tokens_total,
            allocs_total,
            per_total,
            if per_total <= 0.5 { "OK (≤ 0,5)" } else { "ПРОВАЛ (> 0,5)" }
        );
        if per_total > 0.5 {
            return Err(format!("критерий аллокаций провален: {:.3}/токен > 0,5", per_total));
        }
        Ok(())
    }
}
